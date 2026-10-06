import { selectedBehaviorIdForDeployment } from "@source-inc/gents-desktop-client";

import { firstNode, nodeOf } from "./fleetStore";
import { selection } from "./selectionStore";
import type { ShellStores } from "./shellProjection";

/**
 * The selection kept valid against what the nodes list, as either changes:
 * an empty selection starts on the first node listed, and the behavior is
 * settled against the selected node. It runs inside the write that changed
 * the fleet or the selection, so nothing reads the selection before it is
 * valid.
 *
 * Snapshot absence is not an explicit navigation intent: an existing node
 * selection is kept while bounded observations catch up, and an empty one
 * is initialized only through the route owner (`selectAgent`).
 */
export function reconcileSelection(
  stores: Pick<ShellStores, "selection" | "fleet">,
  selectAgent: (agentDid: string) => void,
) {
  const store = stores.selection;
  const settle = () => {
    const { agentDid, behaviorId, composingFor } = store.getState();
    const fleet = stores.fleet.getState();
    if (!agentDid) {
      const first = firstNode(fleet);
      if (first) selectAgent(first.agentDid);
      return;
    }
    const node = nodeOf(fleet, agentDid);
    /* a read that lists no such node (one taken while the client restarts)
       is not a choice either: the behavior, and a mailbox reply armed with
       it, stay until the node is listed again */
    if (!node) return;
    /* a mailbox tap or the new-session screen chose this behavior; it is
       kept while the independently replicated behavior and session rows
       catch up, and explicit navigation lets go of it in the selection store */
    if (composingFor === node.agentDid) return;
    /* a snapshot may reconcile behavior availability, never the session
       selected: null is an intentional fresh composer, not a request to open
       the first matching session, and a missing selected row stays selected
       while hydration and error presentation handle its availability
       (ClientShell's snapshot_preserves_selection contract) */
    selection.settleBehavior(store, selectedBehaviorIdForDeployment(node, behaviorId));
  };
  stores.fleet.subscribe(settle);
  store.subscribe(settle);
  settle();
}
