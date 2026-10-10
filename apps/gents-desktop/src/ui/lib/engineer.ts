/* First-run Engineer: a stable configurator that sets up the user's
   working agent. */
import type { NodeView } from "../../hooks/fleetStore";
import type { ConfigComponentPatch } from "@source-inc/gents-desktop-client";

import sharedSetupPrompt from "../../../../../crates/gents-protocol/prompts/engineer.md?raw";
import setupSelfConfig from "../../../../../crates/gents-protocol/presets/engineer-self-config.json";
import { defaultAgentOf } from "./agents";

export const ENGINEER_PROMPT = sharedSetupPrompt;

export const ENGINEER_DESCRIPTION =
  "Walks you through configuring Gents for the work you want to do.";
export const ENGINEER_AGENT_TAG = "gents:engineer";

export function engineerPatches(deployment: NodeView): ConfigComponentPatch[] {
  const agent =
    deployment.agents.find((row) => row.tags.includes(ENGINEER_AGENT_TAG)) ??
    defaultAgentOf(deployment) ??
    deployment.agents[0];
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
        built_ins: {
          ...deployment.tools.find((tools) => tools.tools_id === toolsId)?.built_ins,
          enable_graph_tools: true,
        },
      },
    });
  }
  return patches;
}
