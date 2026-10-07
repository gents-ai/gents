/* A node's agents (behaviors, in the bridge's current words; gents #1799
   renames them). Every screen finds one through here. */
import {
  selectedBehaviorIdForDeployment,
  type BehaviorView,
} from "@source-inc/gents-desktop-client";

import type { NodeView } from "../../hooks/fleetStore";

/** An agent of a node, by id. */
export function agentOf(
  node: Pick<NodeView, "behaviors"> | null | undefined,
  agentId: string | null | undefined,
): BehaviorView | null {
  if (!node || !agentId) return null;
  return node.behaviors.find((agent) => agent.behaviorId === agentId) ?? null;
}

/** The node's default agent, by the client's one rule: the principal's
    default, then the one marked default, then the conventional id, then
    the first enabled. */
export function defaultAgentOf(node: NodeView | null | undefined): BehaviorView | null {
  return agentOf(node, selectedBehaviorIdForDeployment(node ?? null, null));
}
