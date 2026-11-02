import { selectedAgentIdForDeployment } from "@source-inc/gents-desktop-client";

import { toast } from "sonner";
import { openOnNode } from "../ui/preferences";
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
  let toldMissing = false;
  const settle = () => {
    const { nodeDid, agentId, composingFor } = store.getState();
    const fleet = stores.fleet.getState();
    if (!nodeDid) {
      /* the node chosen to open on, else the one used last on this device,
         else this machine's own, else whichever is listed first */
      const home = stores.client?.getState().snapshot?.bootstrap?.initNodeDid;
      const chosen = openOnNode();
      const start =
        nodeOf(fleet, chosen) ??
        nodeOf(fleet, rememberedNode()) ??
        nodeOf(fleet, home) ??
        firstNode(fleet);
      if (!start) return;
      if (chosen && start.nodeDid !== chosen && !toldMissing) {
        toldMissing = true;
        toast(
          `The node set to open on isn't listed, so the app opened on ${start.node.displayName ?? start.label}`,
        );
      }
      selectNode(start.nodeDid);
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
