import { selectedAgentIdForDeployment } from "@source-inc/gents-desktop-client";

import { chat } from "./chatStore";
import { listedSession, nodeKeyOf, nodeOrFirst, type FleetState } from "./fleetStore";
import { selection, type SelectionState } from "./selectionStore";
import { writeSession } from "./sessionStore";
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
  const dropSession = () => writeSession(stores.session, null);

  function selectNode(nodeDid: string | null) {
    if (selection.selectNode(store, nodeDid)) dropSession();
  }

  function selectAgent(agentId: string | null) {
    selection.selectAgent(store, agentId);
  }

  function selectSession(sessionId: string) {
    const listed = listedSession(fleet(), store.getState().nodeDid, sessionId);
    selection.selectSession(store, sessionId, listed?.agentId);
    if (!listed) dropSession();
  }

  function startNewSession(agentId?: string | null) {
    const node = nodeOrFirst(fleet(), store.getState().nodeDid);
    if (!node) return;
    selection.startNewSession(
      store,
      node.nodeDid,
      selectedAgentIdForDeployment(node, agentId ?? null),
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
      selection.selectSessionOn(store, ownerDid, sessionId, listed?.agentId);
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
