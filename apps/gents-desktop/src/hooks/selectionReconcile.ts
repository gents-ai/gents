import { selectedAgentIdForDeployment } from "@source-inc/gents-desktop-client";

import { firstNode, nodeOf } from "./fleetStore";
import { selection } from "./selectionStore";
import type { ShellStores } from "./shellProjection";

/**
 * The selection kept valid against what the nodes list, as either changes:
 * an empty selection starts on the first node listed, and the agent is
 * settled against the selected node. It runs inside the write that changed
 * the fleet or the selection, so nothing reads the selection before it is
 * valid.
 *
 * Snapshot absence is not an explicit navigation intent: an existing node
 * selection is kept while bounded observations catch up, and an empty one
 * is initialized only through the route owner (`selectNode`).
 */
export function reconcileSelection(
  stores: Pick<ShellStores, "selection" | "fleet">,
  selectNode: (nodeDid: string) => void,
) {
  const store = stores.selection;
  const settle = () => {
    const { nodeDid, agentId, composingFor } = store.getState();
    const fleet = stores.fleet.getState();
    if (!nodeDid) {
      const first = firstNode(fleet);
      if (first) selectNode(first.nodeDid);
      return;
    }
    const node = nodeOf(fleet, nodeDid);
    /* a read that lists no such node (one taken while the client restarts)
       is not a choice either: the agent, and a mailbox reply armed with
       it, stay until the node is listed again */
    if (!node) return;
    /* a mailbox tap or the new-session screen chose this agent; it is
       kept while the independently replicated agent and session rows
       catch up, and explicit navigation lets go of it in the selection store */
    if (composingFor === node.nodeDid) return;
    /* a snapshot may reconcile agent availability, never the session
       selected: null is an intentional fresh composer, not a request to open
       the first matching session, and a missing selected row stays selected
       while hydration and error presentation handle its availability
       (ClientShell's snapshot_preserves_selection contract) */
    selection.settleAgent(store, selectedAgentIdForDeployment(node, agentId));
  };
  stores.fleet.subscribe(settle);
  store.subscribe(settle);
  settle();
}
