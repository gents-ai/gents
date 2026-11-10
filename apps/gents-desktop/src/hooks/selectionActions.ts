import { selectedAgentDidForDeployment } from "@source-inc/gents-desktop-client";

import { chat } from "./chatStore";
import { listedSession, nodeKeyOf, nodeOrFirst, type FleetState } from "./fleetStore";
import { selection, type SelectionState } from "./selectionStore";
import { keepSelectedSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";
import { clientStatus } from "./clientStore";

type SelectionActionParams = {
  stores: ShellStores;
};

export function sessionDestination(
  fleet: FleetState,
  active: Pick<SelectionState, "nodeDid" | "sessionId">,
  sessionId: string,
  explicitNodeDid?: string,
): string | undefined {
  if (explicitNodeDid) return explicitNodeDid;
  if (
    active.nodeDid &&
    (active.sessionId === sessionId || listedSession(fleet, active.nodeDid, sessionId))
  )
    return active.nodeDid;
  const owner = fleet.nodeKeys.find((key) =>
    fleet.sessionsOf[key]?.some((session) => session.sessionId === sessionId),
  );
  return owner ? fleet.nodes[owner]?.nodeDid : undefined;
}

export function createSelectionActions({ stores }: SelectionActionParams) {
  const store = stores.selection;
  const fleet = () => stores.fleet.getState();
  /* a move drops the session held for the selection it left; a recent read
     the move already showed is the new selection's and stays */
  const dropSession = () => {
    const { sessionId, nodeDid } = store.getState();
    keepSelectedSession(stores.session, sessionId, nodeDid);
  };

  function selectNode(nodeDid: string | null) {
    if (selection.selectNode(store, nodeDid)) dropSession();
  }

  function selectAgent(agentDid: string | null) {
    selection.selectAgent(store, agentDid);
  }

  function selectSession(sessionId: string) {
    const listed = listedSession(fleet(), store.getState().nodeDid, sessionId);
    selection.selectSession(store, sessionId, listed?.agentDid);
    if (!listed) dropSession();
  }

  function startNewSession(agentDid?: string | null) {
    const node = nodeOrFirst(fleet(), store.getState().nodeDid);
    if (!node) return;
    selection.startNewSession(
      store,
      node.nodeDid,
      selectedAgentDidForDeployment(node, agentDid ?? null),
    );
    dropSession();
    chat.resetWorkflow(stores.chat);
    clientStatus.setError(stores.client, null);
  }

  function followRoute(
    sessionId: string | null,
    fromSnapshot = false,
    routeNodeDid?: string,
  ) {
    if (sessionId === null) {
      if (routeNodeDid) selectNode(routeNodeDid);
      const state = store.getState();
      if (fromSnapshot || (state.sessionId === null && state.mailboxRoute)) return;
      startNewSession();
      return;
    }
    const state = store.getState();
    const { sessionsOf } = fleet();
    const ownerDid = sessionDestination(fleet(), state, sessionId, routeNodeDid);
    const owner = ownerDid ? nodeKeyOf({ nodeDid: ownerDid }) : undefined;
    if (ownerDid && ownerDid !== state.nodeDid) {
      const listed = owner
        ? sessionsOf[owner]?.find((s) => s.sessionId === sessionId)
        : undefined;
      selection.selectSessionOn(store, ownerDid, sessionId, listed?.agentDid);
      dropSession();
      return;
    }
    if (!owner && fromSnapshot) return;
    if (state.sessionId !== sessionId) selectSession(sessionId);
  }

  return {
    selectNode,
    selectAgent,
    selectSession,
    startNewSession,
    followRoute,
  };
}
