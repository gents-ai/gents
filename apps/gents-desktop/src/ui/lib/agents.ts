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

export function backendOf(
  node: Pick<NodeView, "inferenceProfiles" | "inferenceBackends">,
  agent: Pick<AgentView, "inferenceProfileId">,
) {
  const profile = node.inferenceProfiles.find(
    (p) => p.profile_id === agent.inferenceProfileId,
  );
  if (!profile) return null;
  return node.inferenceBackends.find((b) => b.backendId === profile.backend_id) ?? null;
}

export function defaultAgentOf(node: NodeView | null | undefined): AgentView | null {
  return agentOf(node, selectedAgentIdForDeployment(node ?? null, null));
}
