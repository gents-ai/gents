import { selectedAgentIdForDeployment } from "@source-inc/gents-desktop-client";

import { chat } from "./chatStore";
import { firstNode, listedSession, nodeOf } from "./fleetStore";
import { selection } from "./selectionStore";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";
import { clientStatus } from "./clientStore";

type SelectionActionParams = {
  stores: ShellStores;
};

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
    const node = nodeOf(fleet(), store.getState().nodeDid) ?? firstNode(fleet());
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

  function followRoute(sessionId: string | null, fromSnapshot = false) {
    const state = store.getState();
    if (sessionId === null) {
      if (fromSnapshot || (state.sessionId === null && state.mailboxRoute)) return;
      startNewSession();
      return;
    }
    const { nodeKeys, nodes, sessionsOf } = fleet();
    const owner = nodeKeys.find((key) =>
      sessionsOf[key]?.some((s) => s.sessionId === sessionId),
    );
    const ownerDid = owner ? nodes[owner]?.nodeDid : undefined;
    if (owner && ownerDid && ownerDid !== state.nodeDid) {
      const listed = sessionsOf[owner]?.find((s) => s.sessionId === sessionId);
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
