import { selectedAgentIdForDeployment } from "@source-inc/gents-desktop-client";

import { firstNode, nodeOf } from "./fleetStore";
import { selection } from "./selectionStore";
import type { ShellStores } from "./shellProjection";

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
    if (!node) return;
    if (composingFor === node.nodeDid) return;
    selection.settleAgent(store, selectedAgentIdForDeployment(node, agentId));
  };
  stores.fleet.subscribe(settle);
  store.subscribe(settle);
  settle();
}
