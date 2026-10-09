import {
  projectChatShell,
  type ChatWorkflowState,
} from "@source-inc/gents-desktop-chat";
import {
  isTerminalTurnState,
  projectDeploymentOperationalState,
  selectedBehaviorReadinessDecision,
  type SessionSummary,
  type SyncHealthView,
} from "@source-inc/gents-desktop-client";

import type { ChatStore } from "./chatStore";
import { clientRunning, syncHealthOf, type ClientStore } from "./clientStore";
import { listedSession, nodeOf, type FleetStore, type NodeView } from "./fleetStore";
import type { Selection, SelectionStore } from "./selectionStore";
import {
  headerOf,
  heldFor,
  type SessionHeader,
  type SessionStore,
} from "./sessionStore";

function trackedRequestIdForSession(
  sessionId: string | null,
  workflow: ChatWorkflowState,
) {
  if (!sessionId) {
    return null;
  }

  if (workflow.kind === "awaitingObservation" || workflow.kind === "turnInProgress") {
    return workflow.sessionId === sessionId ? (workflow.requestId ?? null) : null;
  }

  return null;
}

/** Every store the shell is projected from. */
export type ShellStores = {
  selection: SelectionStore;
  session: SessionStore;
  fleet: FleetStore;
  client: ClientStore;
  chat: ChatStore;
};

export type ProjectionInputs = {
  clientAvailable: boolean;
  syncHealth: SyncHealthView | null;
  selection: Selection;
  /** the selected node */
  node: NodeView | null;
  /** the selected session as the node lists it */
  sessionSummary: SessionSummary | null;
  /** the selected session's header, while the held read is the selected one */
  session: SessionHeader | null;
  localWorkflow: ChatWorkflowState;
  sending: boolean;
};

/**
 * What the shell decides with: the node's operational state, the selected
 * behavior's readiness, the chat shell projection for a new message and for
 * a retry under the session's own behavior, and the request being tracked.
 * Pure, so it is the same whether a screen renders it or an action asks.
 */
export function projectShell(inputs: ProjectionInputs) {
  const { node, selection, session, syncHealth } = inputs;
  const operationalState = node
    ? projectDeploymentOperationalState(node, selection.behaviorId, syncHealth)
    : null;
  const behaviorReadiness =
    operationalState?.behaviorReadiness ??
    selectedBehaviorReadinessDecision(null, selection.behaviorId);
  const retryOperationalState = node
    ? projectDeploymentOperationalState(node, session?.behaviorId ?? null, syncHealth)
    : null;
  const base = {
    clientAvailable: inputs.clientAvailable,
    selectedAgentDid: selection.agentDid,
    selectedSessionId: selection.sessionId,
    sending: inputs.sending,
    session,
    selectedSessionSummary: inputs.sessionSummary,
    localWorkflow: inputs.localWorkflow,
  };
  const shellProjection = projectChatShell({ ...base, operationalState });
  const retryShellProjection = projectChatShell({
    ...base,
    operationalState: retryOperationalState,
  });
  const trackedRequestId =
    trackedRequestIdForSession(selection.sessionId, shellProjection.workflow) ??
    (!isTerminalTurnState(shellProjection.turnState)
      ? shellProjection.activeRequestId
      : null);
  return {
    operationalState,
    behaviorReadiness,
    shellProjection,
    retryShellProjection,
    trackedRequestId,
  };
}

export type ShellProjection = ReturnType<typeof projectShell>;

/** The projection's inputs as the stores hold them now, for an action. */
export function projectionInputsOf(stores: ShellStores): ProjectionInputs {
  const selection = stores.selection.getState();
  const fleet = stores.fleet.getState();
  const client = stores.client.getState();
  const chat = stores.chat.getState();
  const held = stores.session.getState().session;
  const agentDid = selection.agentDid;
  return {
    clientAvailable: clientRunning(client),
    syncHealth: syncHealthOf(client),
    selection,
    node: nodeOf(fleet, agentDid),
    sessionSummary: listedSession(fleet, agentDid, selection.sessionId),
    session: headerOf(heldFor(held, selection.sessionId, agentDid)),
    localWorkflow: chat.localWorkflow,
    sending: chat.sending,
  };
}
