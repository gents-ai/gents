import type { NodeView } from "../../hooks/fleetStore";
import type { AgentView, ConfigComponentPatch } from "@source-inc/gents-desktop-client";

import sharedSetupPrompt from "../../../../../crates/gents-protocol/prompts/engineer.md?raw";
import setupSelfConfig from "../../../../../crates/gents-protocol/presets/engineer-self-config.json";
import { defaultAgentOf } from "./agents";

export const ENGINEER_PROMPT = sharedSetupPrompt;

export const ENGINEER_DESCRIPTION =
  "Walks you through configuring Gents for the work you want to do.";
export const ENGINEER_AGENT_TAG = "gents:engineer";

export function engineerAgentOf(deployment: NodeView): AgentView | undefined {
  return (
    deployment.agents.find((row) => row.tags.includes(ENGINEER_AGENT_TAG)) ??
    defaultAgentOf(deployment) ??
    deployment.agents[0]
  );
}

export function engineerPatches(deployment: NodeView): ConfigComponentPatch[] {
  const agent = engineerAgentOf(deployment);
  if (!agent) return [];
  const context = deployment.contexts.find((row) => row.context_id === agent.contextId);
  const toolsId = context?.tools_id;
  const patches: ConfigComponentPatch[] = [
    {
      collection: "Agent",
      id: agent.agentId,
      changes: {
        display_name: "The Engineer",
        description: ENGINEER_DESCRIPTION,
        tags: Array.from(new Set([...(agent.tags ?? []), ENGINEER_AGENT_TAG])),
      },
    },
  ];
  if (context) {
    patches.push({
      collection: "AgentContext",
      id: context.context_id,
      changes: {
        display_name: "The Engineer",
        system_prompt: ENGINEER_PROMPT,
      },
    });
  }
  if (toolsId) {
    patches.push({
      collection: "Tools",
      id: toolsId,
      changes: {
        self_config: structuredClone(setupSelfConfig),
      },
    });
  }
  return patches;
}
