import type { NodeView } from "../../hooks/fleetStore";
import type { AgentView } from "@source-inc/gents-desktop-client";

import { defaultAgentOf } from "./agents";

export const ENGINEER_AGENT_TAG = "gents:engineer";

export function engineerAgentOf(deployment: NodeView): AgentView | undefined {
  return (
    deployment.agents.find((row) => row.tags.includes(ENGINEER_AGENT_TAG)) ??
    defaultAgentOf(deployment) ??
    deployment.agents[0]
  );
}
