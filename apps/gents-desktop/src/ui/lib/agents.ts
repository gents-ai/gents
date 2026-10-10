/* A node's agents. Every screen finds one through here. */
import {
  selectedAgentIdForDeployment,
  type AgentView,
} from "@source-inc/gents-desktop-client";

import type { NodeView } from "../../hooks/fleetStore";

/** An agent of a node, by id. */
export function agentOf(
  node: Pick<NodeView, "agents"> | null | undefined,
  agentId: string | null | undefined,
): AgentView | null {
  if (!node || !agentId) return null;
  return node.agents.find((agent) => agent.agentId === agentId) ?? null;
}

/** The node's default agent, by the client's one rule: the node's
    default, then the one marked default, then the conventional id, then
    the first enabled. */
export function defaultAgentOf(node: NodeView | null | undefined): AgentView | null {
  return agentOf(node, selectedAgentIdForDeployment(node ?? null, null));
}
