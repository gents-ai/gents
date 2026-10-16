import {
  selectedAgentIdForDeployment,
  type AgentView,
} from "@source-inc/gents-desktop-client";

import type { NodeView } from "../../hooks/fleetStore";

export function agentOf(
  node: Pick<NodeView, "agents"> | null | undefined,
  agentId: string | null | undefined,
): AgentView | null {
  if (!node || !agentId) return null;
  return node.agents.find((agent) => agent.agentId === agentId) ?? null;
}

export function defaultAgentOf(node: NodeView | null | undefined): AgentView | null {
  return agentOf(node, selectedAgentIdForDeployment(node ?? null, null));
}
