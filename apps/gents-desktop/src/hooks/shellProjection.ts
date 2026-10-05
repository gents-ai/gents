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
import type { ClientStore } from "./clientStore";
import { trackedRequestIdForSession } from "./desktopShellRuntime";
import type { FleetStore, NodeView } from "./fleetStore";
import type { Selection, SelectionStore } from "./selectionStore";
import { headerOf, type SessionHeader, type SessionStore } from "./sessionStore";

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
  const snapshot = stores.client.getState().snapshot;
  const chat = stores.chat.getState();
  const held = stores.session.getState().session;
  const agentDid = selection.agentDid;
  return {
    clientAvailable: Boolean(snapshot?.client),
    syncHealth: snapshot?.client?.syncHealth ?? null,
    selection,
    node: agentDid ? (fleet.nodes[agentDid] ?? null) : null,
    sessionSummary: summaryIn(fleet, agentDid, selection.sessionId),
    session:
      held?.sessionId === selection.sessionId &&
      (!agentDid || !held.agentDid || held.agentDid === agentDid)
        ? headerOf(held)
        : null,
    localWorkflow: chat.localWorkflow,
    sending: chat.sending,
  };
}

/** The selected session as its node lists it. */
export function summaryIn(
  fleet: ReturnType<FleetStore["getState"]>,
  agentDid: string | null,
  sessionId: string | null,
): SessionSummary | null {
  if (!agentDid || !sessionId) return null;
  return fleet.sessionsOf[agentDid]?.find((s) => s.sessionId === sessionId) ?? null;
}
