import { selectedAgentIdForDeployment } from "@source-inc/gents-desktop-client";

import { rememberedNode, rememberNode } from "./activeNode";
import { firstNode, nodeOf } from "./fleetStore";
import { selection } from "./selectionStore";
import type { ShellStores } from "./shellProjection";

export function reconcileSelection(
  stores: Pick<ShellStores, "selection" | "fleet"> &
    Partial<Pick<ShellStores, "client">>,
  selectNode: (nodeDid: string) => void,
) {
  const store = stores.selection;
  const settle = () => {
    const { nodeDid, agentId, composingFor } = store.getState();
    const fleet = stores.fleet.getState();
    if (!nodeDid) {
      /* the node chosen last on this device, else this machine's own,
         else whichever is listed first */
      const home = stores.client?.getState().snapshot?.bootstrap?.initNodeDid;
      const start =
        nodeOf(fleet, rememberedNode()) ?? nodeOf(fleet, home) ?? firstNode(fleet);
      if (start) selectNode(start.nodeDid);
      return;
    }
    const node = nodeOf(fleet, nodeDid);
    if (!node) return;
    if (composingFor === node.nodeDid) return;
    selection.settleAgent(store, selectedAgentIdForDeployment(node, agentId));
  };
  stores.fleet.subscribe(settle);
  store.subscribe(settle);
  let kept = store.getState().nodeDid;
  store.subscribe(({ nodeDid }) => {
    if (!nodeDid || nodeDid === kept) return;
    kept = nodeDid;
    rememberNode(nodeDid);
  });
  settle();
}
