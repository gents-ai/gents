import { useMemo } from "react";
import { agentKey, useHiddenAgents, useHiddenNodes } from "@/preferences";
import { useFleet } from "@/hooks/useFleet";
import { fleetNodes } from "./scope";

/** What this device leaves out of the menus that offer nodes and agents:
    the node switcher, the nav's nodes, search, and the agent picker. A
    hidden node or agent is still on its own page, still opens from a link
    or a session, and its mailbox items still reach the Mailbox. */
export type Hidden = {
  node: (nodeDid: string) => boolean;
  /** the agent itself; a menu across nodes also leaves out a hidden
      node's agents */
  agent: (nodeDid: string, agentId: string) => boolean;
};

export function useHidden(): Hidden {
  const nodes = useHiddenNodes();
  const agents = useHiddenAgents();
  return useMemo(
    () => ({
      node: (nodeDid) => nodes.includes(nodeDid),
      agent: (nodeDid, agentId) => agents.includes(agentKey(nodeDid, agentId)),
    }),
    [nodes, agents],
  );
}

/** The nodes a menu offers: the hidden ones are left out, except the one in
    use, which stays so the menu still shows where the app is. */
export const shownNodes = <T extends { nodeDid: string }>(
  nodes: readonly T[],
  hidden: Hidden,
  current: string | null,
): T[] => nodes.filter((n) => n.nodeDid === current || !hidden.node(n.nodeDid));

/** How many nodes a menu would offer, as `shownNodes` counts them. */
export function useShownNodeCount(current: string | null) {
  const hidden = useHidden();
  return useFleet((state) => shownNodes(fleetNodes(state), hidden, current).length);
}
