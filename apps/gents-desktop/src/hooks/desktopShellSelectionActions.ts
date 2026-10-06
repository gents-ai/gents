import { selectedBehaviorIdForDeployment } from "@source-inc/gents-desktop-client";

import { setterOf } from "./chatStore";
import { selection } from "./selectionStore";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";

type SelectionActionParams = {
  stores: ShellStores;
  setError: (error: string | null) => void;
};

/**
 * Every way the person moves between nodes, sessions and behaviors. Each
 * reads the selection and the fleet when it runs, so none holds a stale
 * copy, and each drops the session the screen showed when it no longer
 * applies.
 */
export function createDesktopShellSelectionActions({
  stores,
  setError,
}: SelectionActionParams) {
  const store = stores.selection;
  const fleet = () => stores.fleet.getState();
  const dropSession = () => writeSession(stores.session, null);

  /** the session as the selected node lists it */
  const listedOnSelected = (sessionId: string) => {
    const agentDid = store.getState().agentDid;
    return agentDid
      ? fleet().sessionsOf[agentDid]?.find((s) => s.sessionId === sessionId)
      : undefined;
  };

  function selectAgent(agentDid: string | null) {
    if (selection.selectAgent(store, agentDid)) dropSession();
  }

  function selectBehavior(behaviorId: string | null) {
    selection.selectBehavior(store, behaviorId);
  }

  function selectSession(sessionId: string) {
    const listed = listedOnSelected(sessionId);
    selection.selectSession(store, sessionId, listed?.behaviorId);
    if (!listed) dropSession();
  }

  function startNewSession(behaviorId?: string | null) {
    const { nodes, nodeKeys } = fleet();
    const selected = store.getState().agentDid;
    const first = nodeKeys[0];
    const node =
      (selected ? nodes[selected] : undefined) ?? (first ? nodes[first] : undefined);
    if (!node) return;
    selection.startNewSession(
      store,
      node.agentDid,
      selectedBehaviorIdForDeployment(node, behaviorId ?? null),
    );
    dropSession();
    setterOf(stores.chat, "localWorkflow")({ kind: "ready" });
    setError(null);
    /* the banner may hold the session left behind (a read that failed);
       the new session starts without it */
    stores.client.setState({ error: null });
  }

  function followRoute(sessionId: string | null, fromSnapshot = false) {
    const state = store.getState();
    if (sessionId === null) {
      /* a mailbox item opened into a new session already holds it, with
         the item as the next message's cause */
      if (fromSnapshot || (state.sessionId === null && state.mailboxRoute)) return;
      startNewSession();
      return;
    }
    const { nodeKeys, nodes, sessionsOf } = fleet();
    /* the first node, in the snapshot's order, that lists it */
    const owner = nodeKeys.find((key) =>
      sessionsOf[key]?.some((s) => s.sessionId === sessionId),
    );
    const ownerDid = owner ? nodes[owner]?.agentDid : undefined;
    if (owner && ownerDid && ownerDid !== state.agentDid) {
      const listed = sessionsOf[owner]?.find((s) => s.sessionId === sessionId);
      selection.selectSessionOn(store, ownerDid, sessionId, listed?.behaviorId);
      dropSession();
      return;
    }
    if (!owner && fromSnapshot) return;
    if (state.sessionId !== sessionId) selectSession(sessionId);
  }

  return {
    /**
     * Selects a node; its session and behavior start over and the session
     * shown is dropped. Choosing the node already selected changes nothing,
     * and a snapshot never picks a session for the new node.
     */
    selectAgent,
    /**
     * Selects the behavior the next message goes to. A navigation: it lets go
     * of a mailbox item the message was going to answer.
     */
    selectBehavior,
    /**
     * Selects a session on the selected node. When the node lists it, the
     * behavior it was held under comes back with it; when it does not yet, the
     * session shown is dropped until a read finds it.
     */
    selectSession,
    /**
     * Opens the new-session screen on the selected node, or on the first node
     * while none is selected, with the behavior asked for or the node's
     * default. Drops the session shown and any local workflow.
     */
    startNewSession,
    /**
     * Selects what a route asks for: the new-session screen for null, else
     * that session on the node that lists it, node and session together. A
     * session already selected is left alone, since the route is catching up
     * with a selection made a moment ago (a mailbox item that opened its
     * session) and selecting it again would reset what was set up for it; for
     * the same reason an empty route keeps a mailbox item already opened into
     * a new session. Called again whenever a snapshot lands (fromSnapshot), as
     * a session on another node can only be followed once that node lists it;
     * then it acts only on a listed session and never restarts the new-session
     * screen.
     */
    followRoute,
  };
}
