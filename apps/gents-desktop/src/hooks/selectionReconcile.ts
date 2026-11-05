import { selectedAgentIdForDeployment } from "@source-inc/gents-desktop-client";

import { toast } from "sonner";
import { routeMoves } from "../ui/lib/router";
import { openOnNode } from "../ui/preferences";
import { noteActiveNode, rememberedNode, rememberNode } from "./activeNode";
import type { DraftStore } from "./draftStore";
import { firstNode, type FleetState, nodeOf, type NodeView } from "./fleetStore";
import { selection } from "./selectionStore";
import type { ShellStores } from "./shellProjection";

/** How long after launch a chosen node that isn't there yet, or whose
    reachability isn't known yet, may still arrive and be opened on. */
export const OPEN_ON_WAIT_MS = 5000;

/* a dial that failed leaves its error; before the first dial there is
   neither a connection nor an error */
const reachOf = (node: NodeView) =>
  node.dialSucceeded ? "reachable" : node.lastError ? "failed" : "unknown";

const nameOf = (node: NodeView) => node.node.displayName ?? node.label;

/* the node used last on this device, else this machine's own, else the
   first listed, passing over the chosen node the app couldn't open on */
function fallback(fleet: FleetState, home: string | null | undefined, chosen: string) {
  const usable = (node: NodeView | null) =>
    node && node.nodeDid !== chosen ? node : null;
  return (
    usable(nodeOf(fleet, rememberedNode())) ??
    usable(nodeOf(fleet, home)) ??
    fleet.nodeKeys
      .map((key) => fleet.nodes[key])
      .find((n) => n && n.nodeDid !== chosen) ??
    null
  );
}

export function reconcileSelection(
  stores: Pick<ShellStores, "selection" | "fleet"> &
    Partial<Pick<ShellStores, "client">>,
  selectNode: (nodeDid: string) => void,
  drafts?: DraftStore,
) {
  const store = stores.selection;
  /* a chosen node that may still arrive: what the app opened on meanwhile,
     and the route changes seen when it did */
  let waiting: { chosen: string; openedOn: string; moves: number } | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const stopWaiting = () => {
    waiting = null;
    if (timer) clearTimeout(timer);
    timer = null;
  };
  const tell = (chosen: string, fleet: FleetState, openedOn: string) => {
    const node = nodeOf(fleet, chosen);
    const here = nodeOf(fleet, openedOn);
    const where = here ? nameOf(here) : "another node";
    toast(
      node
        ? `${nameOf(node)} isn't reachable, so the app opened on ${where}`
        : `The startup node isn't listed, so the app opened on ${where}`,
    );
  };
  /* the person has begun: moved, picked a node or typed a draft; the app
     then stays where they are */
  const begun = (w: NonNullable<typeof waiting>) =>
    routeMoves() !== w.moves ||
    store.getState().nodeDid !== w.openedOn ||
    Object.keys(drafts?.getState().drafts ?? {}).length > 0;

  const watch = () => {
    const w = waiting;
    if (!w) return;
    const fleet = stores.fleet.getState();
    const node = nodeOf(fleet, w.chosen);
    const reach = node ? reachOf(node) : null;
    if (reach === "failed") {
      stopWaiting();
      tell(w.chosen, fleet, w.openedOn);
      return;
    }
    if (reach !== "reachable") return;
    stopWaiting();
    if (!begun(w)) selectNode(w.chosen);
  };

  const settle = () => {
    const { nodeDid, agentId, composingFor } = store.getState();
    const fleet = stores.fleet.getState();
    if (!nodeDid) {
      const home = stores.client?.getState().snapshot?.bootstrap?.initNodeDid;
      const chosen = openOnNode();
      const node = nodeOf(fleet, chosen);
      if (chosen && node && reachOf(node) === "reachable") return selectNode(chosen);
      /* with no node chosen: the one used last on this device, else this
         machine's own, else whichever is listed first */
      const start = chosen
        ? (fallback(fleet, home, chosen) ?? node)
        : (nodeOf(fleet, rememberedNode()) ?? nodeOf(fleet, home) ?? firstNode(fleet));
      if (!start) return;
      selectNode(start.nodeDid);
      if (!chosen || start.nodeDid === chosen) return;
      if (node && reachOf(node) === "failed") return tell(chosen, fleet, start.nodeDid);
      /* not listed yet, or not dialled yet: it may still come */
      waiting = { chosen, openedOn: start.nodeDid, moves: routeMoves() };
      timer = setTimeout(() => {
        const w = waiting;
        if (!w) return;
        stopWaiting();
        tell(w.chosen, stores.fleet.getState(), w.openedOn);
      }, OPEN_ON_WAIT_MS);
      return;
    }
    const node = nodeOf(fleet, nodeDid);
    if (!node) return;
    if (composingFor === node.nodeDid) return;
    selection.settleAgent(store, selectedAgentIdForDeployment(node, agentId));
  };
  stores.fleet.subscribe(() => {
    watch();
    settle();
  });
  store.subscribe(settle);
  let kept = store.getState().nodeDid;
  if (kept) noteActiveNode(kept);
  store.subscribe(({ nodeDid }) => {
    if (!nodeDid || nodeDid === kept) return;
    kept = nodeDid;
    rememberNode(nodeDid);
    noteActiveNode(nodeDid);
  });
  settle();
}
